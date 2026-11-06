//! 仓库历史聚合的真源：本地扫历史侧文件元数据，空间走服务端聚合端点；
//! 按文件分组后供 `history.groups` / `history.day` / `history.activity` 回执，
//! 历史弹层本身的版本浏览与回滚走 `history.open`。

use iced::Task;

use atelyx_core::history::{HistoryAction, HistoryKind};
use atelyx_core::plugin_runtime::{
    DayActivity, HistoryActivityResult, HistoryDayEntry, HistoryDayResult, HistoryFileGroup,
    HistoryGroupsParams, HistoryGroupsResult,
};
use atelyx_core::space_client::HistoryAggregate;

use crate::settings_view::plugins::PendingHistory;
use crate::space_actions;
use crate::ui::notify::Level as NoticeLevel;
use crate::{App, Message, RepoKind};

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

/// 历史聚合缓存：分组列表 + 全量版本时间戳（按日计数用，不受条目流条数上限影响）。
#[derive(Debug, Clone)]
pub struct RepoHistory {
    pub groups: Vec<RepoHistoryGroup>,
    pub timestamps: Vec<i64>,
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

/// wire kind 字段 → HistoryKind（未知值 `None`：聚合条目丢弃、历史打开请求回绝）。
pub(crate) fn parse_kind(s: &str) -> Option<HistoryKind> {
    match s {
        "note" => Some(HistoryKind::Note),
        "canvas" => Some(HistoryKind::Canvas),
        "table" => Some(HistoryKind::Table),
        _ => None,
    }
}

/// HistoryKind → wire kind 字段（与服务端聚合同一取值域）。
pub(crate) fn kind_key(kind: HistoryKind) -> &'static str {
    match kind {
        HistoryKind::Note => "note",
        HistoryKind::Canvas => "canvas",
        HistoryKind::Table => "table",
    }
}

/// HistoryAction → wire action 字段。
pub(crate) fn action_key(action: HistoryAction) -> &'static str {
    match action {
        HistoryAction::Edit => "edit",
        HistoryAction::Restore => "restore",
    }
}

/// 服务端 action 字段 → HistoryAction（服务端仅写 edit/restore，其余按 edit 展示）。
fn parse_action(s: &str) -> HistoryAction {
    if s == "restore" {
        HistoryAction::Restore
    } else {
        HistoryAction::Edit
    }
}

/// 当日版本记录（按本地日界过滤，ts 毫秒；day = 自纪元天数），新→旧。
pub(crate) fn day_entries(groups: &[RepoHistoryGroup], day: i64) -> Vec<HistoryDayEntry> {
    let mut out: Vec<HistoryDayEntry> = groups
        .iter()
        .flat_map(|group| {
            group
                .versions
                .iter()
                .filter(|version| atelyx_core::history::day_key(version.ts) == day)
                .map(move |version| HistoryDayEntry {
                    kind: kind_key(group.kind).to_string(),
                    file: group.file.clone(),
                    ts: version.ts,
                    action: action_key(version.action).to_string(),
                    author_name: version.author_name.clone(),
                    summary: version.summary.clone(),
                })
        })
        .collect();
    out.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
    out
}

/// 当前仓库容器是否就绪（本地仓库根可用 / 空间会话在位；仓库历史与日历共用）。
pub(crate) fn container_ready(app: &App) -> bool {
    match &app.repo_kind {
        RepoKind::Local => app.vault.root().is_ok(),
        RepoKind::Space(_) => space_actions::space_key_of(app).is_some(),
    }
}

/// 域插件落盘后记一版（本地侧文件）。空间模式的历史由空间补丁服务在保存链内记，
/// 这里直接跳过；失败经 `HistoryRecorded` 提示并刷新弹层。
pub fn record_from_plugin(
    app: &mut App,
    params: atelyx_core::plugin_runtime::HistoryRecordParams,
) -> Task<Message> {
    let kind = match params.kind.as_str() {
        "note" => HistoryKind::Note,
        "canvas" => HistoryKind::Canvas,
        "table" => HistoryKind::Table,
        other => {
            app.notify(NoticeLevel::Error, format!("未知的历史类型：{other}"));
            return Task::none();
        }
    };
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        return Task::none();
    }
    let action = match params.action.as_deref() {
        Some("restore") => HistoryAction::Restore,
        _ => HistoryAction::Edit,
    };
    let gen = app.epoch();
    let state = app.vault.clone();
    let author = crate::history_author(app);
    let file = params.file;
    let opts = atelyx_core::history::RecordOpts {
        content: params.content,
        action,
        author,
        coalesce_edit_ms: params.coalesce_ms,
        max_versions: 0,
        note: params.note,
    };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let (root, _, _) = state.session_snapshot()?;
                atelyx_core::history::record_version(&root, kind, &file, &opts)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        },
        move |result| Message::HistoryRecorded(gen, result.err()),
    )
}

pub fn load(app: &mut App) -> Task<Message> {
    app.repo_history_loading = true;
    let epoch = app.epoch();
    let gen = app.repo_history_gen;
    match &app.repo_kind {
        RepoKind::Space(session) => {
            let client = session.client.clone();
            let space_id = session.space_id.clone();
            Task::perform(
                async move {
                    let agg = client.history_aggregate(&space_id).await.map_err(|e| e.to_string())?;
                    let timestamps = agg.timestamps.clone();
                    Ok(RepoHistory { groups: groups_from_space(agg), timestamps })
                },
                move |result| Message::RepoHistoryLoaded(epoch, gen, result),
            )
        }
        RepoKind::Local => {
            let state = app.vault.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let (root, _, _) = state.session_snapshot()?;
                        atelyx_core::history::collect_all(&root).map(|files| {
                            let groups = groups_from_local(files);
                            // 本地无条目上限：分组里就是全量版本
                            let timestamps = groups
                                .iter()
                                .flat_map(|group| group.versions.iter().map(|version| version.ts))
                                .collect();
                            RepoHistory { groups, timestamps }
                        })
                    })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
                },
                move |result| Message::RepoHistoryLoaded(epoch, gen, result),
            )
        }
    }
}

/// 丢弃聚合缓存并推进代次：在途扫描落地时按代次作废，不把失效前的聚合写回。
pub(crate) fn invalidate(app: &mut App) {
    app.repo_history = None;
    app.repo_history_gen = app.repo_history_gen.wrapping_add(1);
}

/// 聚合落地（世代守卫）：成功缓存列表；失败缓存 Err 并提示（失败不自动重试，改由调用方重试）。
/// 等这份缓存的查询在此一并回填。
pub fn on_loaded(
    app: &mut App,
    epoch: u64,
    gen: u64,
    result: Result<RepoHistory, String>,
) -> Task<Message> {
    app.repo_history_loading = false;
    if epoch != app.epoch() {
        // 容器已变：在途结果作废，挂起的查询已在切容器时回绝
        return Task::none();
    }
    if gen != app.repo_history_gen {
        // 扫描启动后缓存又被丢弃：这份结果作废；仍有查询等着就按当前状态重扫
        if app.plugins_state.pending_history.is_empty() {
            return Task::none();
        }
        return load(app);
    }
    let pending = std::mem::take(&mut app.plugins_state.pending_history);
    match result {
        Ok(history) => {
            for query in pending {
                let answer = match query {
                    PendingHistory::Groups(request_id) => (request_id, groups_json(&history)),
                    PendingHistory::Day(request_id, day) => {
                        (request_id, history_day_json(day_entries(&history.groups, day)))
                    }
                    PendingHistory::Activity(request_id) => {
                        (request_id, activity_json(&history.timestamps))
                    }
                };
                app.plugins.resolve_shell_call(answer.0, answer.1);
            }
            app.repo_history = Some(Ok(history));
        }
        Err(e) => {
            for query in pending {
                app.plugins
                    .resolve_shell_call(query.request_id(), Err(format!("历史聚合失败：{e}")));
            }
            app.repo_history = Some(Err(e.clone()));
            app.notify(NoticeLevel::Error, format!("历史聚合失败：{e}"));
        }
    }
    Task::none()
}

/// `history.groups` 的回执：容器就绪时的全部分组（未就绪走 `HistoryGroupsResult::default()`）。
pub(crate) fn groups_json(history: &RepoHistory) -> Result<serde_json::Value, String> {
    let groups = history
        .groups
        .iter()
        .map(|group| HistoryFileGroup {
            kind: kind_key(group.kind).to_string(),
            file: group.file.clone(),
            count: group.versions.len() as u32,
        })
        .collect();
    serde_json::to_value(HistoryGroupsResult { ready: true, groups })
        .map_err(|e| format!("历史分组不可序列化：{e}"))
}

/// 按日查询的宿主回执（`history.day`）；序列化失败带回可读原因。
pub(crate) fn history_day_json(entries: Vec<HistoryDayEntry>) -> Result<serde_json::Value, String> {
    serde_json::to_value(HistoryDayResult { entries })
        .map_err(|e| format!("历史结果不可序列化：{e}"))
}

/// 按天聚合的活动计数（`history.activity` 的协作空间口径）：版本时间戳按本地日界归日。
pub(crate) fn activity_json(timestamps: &[i64]) -> Result<serde_json::Value, String> {
    let mut counts: std::collections::BTreeMap<i64, u32> = std::collections::BTreeMap::new();
    for ts in timestamps {
        *counts.entry(atelyx_core::history::day_key(*ts)).or_default() += 1;
    }
    let days: Vec<DayActivity> =
        counts.into_iter().map(|(day, count)| DayActivity { day, count }).collect();
    serde_json::to_value(HistoryActivityResult { days })
        .map_err(|e| format!("活动统计不可序列化：{e}"))
}

/// `history.groups` 的请求入口：缓存缺失时挂起（聚合一落地就回填），在途不重复发起。
/// `params.refresh` = 丢弃缓存重扫（手动刷新 / 重试用；扫描在途也按新世代重扫）；
/// 容器未就绪回 `ready: false`。
pub(crate) fn request_groups(
    app: &mut App,
    request_id: u64,
    params: HistoryGroupsParams,
) -> Task<Message> {
    if !container_ready(app) {
        let result = serde_json::to_value(HistoryGroupsResult::default())
            .map_err(|e| format!("历史分组不可序列化：{e}"));
        app.plugins.resolve_shell_call(request_id, result);
        return Task::none();
    }
    if params.refresh {
        invalidate(app);
    }
    if let Some(cached) = &app.repo_history {
        let result = match cached {
            Ok(history) => groups_json(history),
            Err(e) => Err(format!("历史聚合失败：{e}")),
        };
        app.plugins.resolve_shell_call(request_id, result);
        return Task::none();
    }
    app.plugins_state
        .pending_history
        .push(PendingHistory::Groups(request_id));
    if app.repo_history_loading {
        // 在途扫描的世代已被 refresh 抛在后面：等它落地时按代次丢弃并重扫
        return Task::none();
    }
    load(app)
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

    #[test]
    fn groups_payload_counts_versions_and_keeps_order() {
        let history = RepoHistory {
            groups: vec![
                RepoHistoryGroup {
                    kind: HistoryKind::Note,
                    file: "a.md".into(),
                    versions: vec![
                        RepoVersionRow {
                            ts: 300,
                            action: HistoryAction::Edit,
                            author_name: "alice".into(),
                            summary: None,
                        },
                        RepoVersionRow {
                            ts: 200,
                            action: HistoryAction::Restore,
                            author_name: "alice".into(),
                            summary: None,
                        },
                    ],
                },
                RepoHistoryGroup {
                    kind: HistoryKind::Canvas,
                    file: "b.atlx".into(),
                    versions: vec![RepoVersionRow {
                        ts: 100,
                        action: HistoryAction::Edit,
                        author_name: "bob".into(),
                        summary: None,
                    }],
                },
            ],
            timestamps: vec![300, 200, 100],
        };
        let value = groups_json(&history).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "ready": true,
                "groups": [
                    { "kind": "note", "file": "a.md", "count": 2 },
                    { "kind": "canvas", "file": "b.atlx", "count": 1 }
                ]
            })
        );
    }
}
