//! 最近打开视图：文件确认打开成功后按仓库容器身份记录（ui-state 透传字段），
//! 面板内按当前身份过滤展示，并定期校验条目存在性剔除失效项。

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, row, space};
use iced::{Element, Length, Padding, Task};

use serde::{Deserialize, Serialize};

use atelyx_core::layout::{AppUiState, MAX_RECENT_FILES};
use atelyx_core::vault;

use crate::space_actions;
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::text;
use crate::{App, Message, RepoKind};

/// 最近打开的文件类型。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecentKind {
    Note,
    Table,
    Canvas,
}

/// 最近打开条目（ui-state `recentFiles` 透传字段的磁盘结构，camelCase 契约）。
/// `vaultKey`：本地 = 仓库根路径字符串；空间 = 服务器地址/空间 id（与展开集记忆同源）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentFile {
    pub file: String,
    pub kind: RecentKind,
    pub vault_key: String,
    pub opened_at: i64,
}

/// 条目存在性校验的发起间隔（视图展示期间搭载 PersistTick 节流）。
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// 去重置顶 + 截断：同 (file, vaultKey) 旧条目移除，新条目插到最前，超限从尾部截。
fn push_recent(mut list: Vec<RecentFile>, entry: RecentFile, max: usize) -> Vec<RecentFile> {
    list.retain(|e| e.file != entry.file || e.vault_key != entry.vault_key);
    list.insert(0, entry);
    list.truncate(max);
    list
}

/// 按仓库身份过滤 + openedAt 倒序（磁盘条目可能乱序，展示前统一排序）。
fn recent_for(list: &[RecentFile], vault_key: &str) -> Vec<RecentFile> {
    let mut out: Vec<RecentFile> =
        list.iter().filter(|e| e.vault_key == vault_key).cloned().collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.opened_at));
    out
}

/// 透传字段 → 条目列表（旧数据或不可识别条目丢弃）。
fn decode(ui: &AppUiState) -> Vec<RecentFile> {
    ui.recent_files
        .iter()
        .filter_map(|v| serde_json::from_value::<RecentFile>(v.clone()).ok())
        .collect()
}

/// 条目列表写回透传字段并置脏落盘。
fn encode(app: &mut App, list: Vec<RecentFile>) {
    app.ui.recent_files =
        list.into_iter().map(|e| serde_json::to_value(e).unwrap_or_default()).collect();
    app.schedule_persist();
}

/// 当前仓库容器身份键（记录与过滤同源）：本地 = 仓库根路径串；空间 = 服务器/空间 id。
fn vault_key_of(app: &App) -> Option<String> {
    match &app.repo_kind {
        RepoKind::Local => app.vault.root().ok().map(|p| p.to_string_lossy().into_owned()),
        RepoKind::Space(_) => space_actions::space_key_of(app),
    }
}

/// 文件确认打开成功后记录（去重置顶 + 上限截断 + 置脏落盘）；时刻 = 当前秒。
/// 无身份键（仓库未就绪）不记录。
pub fn record(app: &mut App, file: &str, kind: RecentKind) {
    let Some(vault_key) = vault_key_of(app) else { return };
    let entry = RecentFile {
        file: file.to_string(),
        kind,
        vault_key,
        opened_at: crate::now_secs() as i64,
    };
    encode(app, push_recent(decode(&app.ui), entry, MAX_RECENT_FILES));
}

/// 剔除确认缺失的条目（按 file + vaultKey 匹配）并置脏落盘。
pub fn prune(app: &mut App, gone: &[(String, String)]) {
    let kept: Vec<RecentFile> = decode(&app.ui)
        .into_iter()
        .filter(|e| !gone.iter().any(|(key, file)| key == &e.vault_key && file == &e.file))
        .collect();
    encode(app, kept);
}

/// PersistTick 搭载：视图展示中且距上次校验超过间隔时发起存在性校验。
pub fn tick(app: &mut App) -> Task<Message> {
    if app.recent_checked_at.is_some_and(|at| at.elapsed() < CHECK_INTERVAL) {
        return Task::none();
    }
    if !crate::layout_view::view_displayed(app, "recent") {
        return Task::none();
    }
    let entries = decode(&app.ui);
    if entries.is_empty() {
        return Task::none();
    }
    app.recent_checked_at = Some(Instant::now());
    prune_space_entries(app, &entries);
    start_local_check(app)
}

/// 空间条目剔除：当前会话整树已在内存，直接比对文件路径；
/// 非当前空间的条目不可校验，保留（等该空间激活时再判）。
fn prune_space_entries(app: &mut App, entries: &[RecentFile]) {
    let RepoKind::Space(session) = &app.repo_kind else { return };
    let Some(tree) = &app.space_tree else { return };
    let mut paths: HashSet<&str> = HashSet::new();
    collect_tree_paths(tree, &mut paths);
    let key = format!("{}/{}", session.server_url, session.space_id);
    let gone: Vec<(String, String)> = entries
        .iter()
        .filter(|e| e.vault_key == key && !paths.contains(e.file.as_str()))
        .map(|e| (e.vault_key.clone(), e.file.clone()))
        .collect();
    if !gone.is_empty() {
        prune(app, &gone);
    }
}

/// 展平空间整树的全部文件路径。
fn collect_tree_paths<'a>(tree: &'a [atelyx_core::space_client::TreeNode], out: &mut HashSet<&'a str>) {
    for node in tree {
        if !node.is_dir {
            out.insert(node.path.as_str());
        }
        collect_tree_paths(&node.children, out);
    }
}

/// 本地条目存在性校验（spawn_blocking 逐条判存在；键为绝对仓库根路径，
/// 空间条目键以协议前缀区分）。只剔除确认缺失的条目：safe_join 失败 = 无法校验，保留。
fn start_local_check(app: &App) -> Task<Message> {
    let checks: Vec<(String, String)> = decode(&app.ui)
        .into_iter()
        .filter(|e| !e.vault_key.starts_with("http://") && !e.vault_key.starts_with("https://"))
        .map(|e| (e.vault_key, e.file))
        .collect();
    if checks.is_empty() {
        return Task::none();
    }
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                checks
                    .into_iter()
                    .filter(|(key, file)| {
                        vault::safe_join(Path::new(key), file, false)
                            .map(|p| !p.exists())
                            .unwrap_or(false)
                    })
                    .collect::<Vec<(String, String)>>()
            })
            .await
            .unwrap_or_default()
        },
        Message::RecentPruned,
    )
}

/// 秒级时间戳 → 相对文案：刚刚 / N 分钟前 / N 小时前 / N 天前 / 日期（YYYY-MM-DD）。
pub(crate) fn relative_ts(secs: i64, now: i64) -> String {
    let d = now - secs;
    if d < 60 {
        return "刚刚".into();
    }
    if d < 3600 {
        return format!("{} 分钟前", d / 60);
    }
    if d < 86_400 {
        return format!("{} 小时前", d / 3600);
    }
    if d < 30 * 86_400 {
        return format!("{} 天前", d / 86_400);
    }
    crate::layout_view::format_ts(secs * 1000)
        .split(' ')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// 最近打开视图：按当前仓库身份过滤，行点击打开（复用 OpenFile，按扩展名分流笔记/表格/画布）。
pub fn recent_view(app: &App) -> Element<'_, Message> {
    let Some(key) = vault_key_of(app) else {
        return controls::empty_state("clock", "未打开仓库", "打开仓库后显示", None);
    };
    let items = recent_for(&decode(&app.ui), &key);
    if items.is_empty() {
        return controls::empty_state(
            "clock",
            "暂无最近打开",
            "打开文件后出现在这里",
            None,
        );
    }
    let now = crate::now_secs() as i64;
    let mut col = iced::widget::Column::<Message>::new()
        .padding(Padding::ZERO.horizontal(12.0).vertical(10.0))
        .spacing(6);
    for e in items {
        let name = e.file.rsplit('/').next().unwrap_or(&e.file).to_string();
        let parent = e.file.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        let time = relative_ts(e.opened_at, now);
        let kind_icon = match e.kind {
            RecentKind::Note => "file-text",
            RecentKind::Table => "table",
            RecentKind::Canvas => "palette",
        };
        col = col.push(
            container(
                button(
                    row![
                        icon(kind_icon, 14.0, pal::TEXT_MUTED),
                        column![
                            text(name)
                                .size(pal::fs_body())
                                .color(pal::TEXT_PRIMARY),
                            row![
                                text(parent).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                                space().width(Length::Fill),
                                text(time).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                            ]
                            .width(Length::Fill),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                    ]
                    .spacing(10)
                    .align_y(iced::alignment::Vertical::Center),
                )
                .width(Length::Fill)
                .padding(0.0)
                .style(controls::row_style(false, pal::RADIUS_MD))
                .on_press(Message::OpenFile(e.file.clone())),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(10.0).vertical(8.0))
            .style(|_t| iced::widget::container::Style {
                background: Some(pal::BG_TERTIARY.into()),
                border: iced::border::rounded(pal::RADIUS_MD),
                ..iced::widget::container::Style::default()
            }),
        );
    }
    scroll::v(container(col).width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, key: &str, at: i64) -> RecentFile {
        RecentFile {
            file: file.into(),
            kind: RecentKind::Note,
            vault_key: key.into(),
            opened_at: at,
        }
    }

    #[test]
    fn push_dedups_moves_to_front() {
        let list = vec![entry("a.md", "k", 1), entry("b.md", "k", 2), entry("c.md", "k", 3)];
        let out = push_recent(list, entry("b.md", "k", 9), 50);
        assert_eq!(
            out.iter().map(|e| (e.file.as_str(), e.opened_at)).collect::<Vec<_>>(),
            [("b.md", 9), ("a.md", 1), ("c.md", 3)]
        );
    }

    #[test]
    fn push_keeps_other_vault_entry() {
        // 同名文件在不同仓库是不同条目，不得互删
        let list = vec![entry("a.md", "k1", 1)];
        let out = push_recent(list, entry("a.md", "k2", 9), 50);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn push_truncates_to_max() {
        // 真实列表形态 = 新→旧；截断从尾部（最旧）丢
        let list = vec![entry("b.md", "k", 2), entry("a.md", "k", 1)];
        let out = push_recent(list, entry("c.md", "k", 3), 2);
        assert_eq!(out.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(), ["c.md", "b.md"]);
    }

    #[test]
    fn recent_for_filters_by_vault_and_sorts_desc() {
        let list = vec![entry("a.md", "k1", 5), entry("b.md", "k2", 9), entry("c.md", "k1", 1)];
        let out = recent_for(&list, "k1");
        assert_eq!(out.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(), ["a.md", "c.md"]);
    }

    #[test]
    fn relative_ts_ladder() {
        let now = 1_000_000_000;
        assert_eq!(relative_ts(now, now), "刚刚");
        assert_eq!(relative_ts(now - 120, now), "2 分钟前");
        assert_eq!(relative_ts(now - 7_200, now), "2 小时前");
        assert_eq!(relative_ts(now - 3 * 86_400, now), "3 天前");
        assert_eq!(relative_ts(now + 100, now), "刚刚");
        let old = relative_ts(now - 40 * 86_400, now);
        assert_eq!(old.len(), "YYYY-MM-DD".len());
        assert!(old.contains('-'));
    }
}
