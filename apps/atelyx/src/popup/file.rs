//! 文件 / 最近仓库 / 排序弹层：右键菜单与移除确认。

use iced::widget::column;
use iced::{Length, Padding};

use crate::layout_view::PopupState;
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, DraftKind, Message};

pub(super) fn entries<'a>(app: &'a App, state: &PopupState, window: iced::window::Id) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::SortMenu => {
            const OPTIONS: [(&str, &str); 4] = [
                ("name-asc", "名称 (A-Z)"),
                ("name-desc", "名称 (Z-A)"),
                ("mtime-desc", "编辑时间 (从新到旧)"),
                ("mtime-asc", "编辑时间 (从旧到新)"),
            ];
            OPTIONS
                .iter()
                .map(|(key, label)| {
                    (
                        0,
                        MenuEntry::Action {
                            label: (*label).to_string(),
                            icon: None,
                            danger: false,
                            disabled: false,
                            on_press: Some(Message::SetSort((*key).to_string())),
                            note: (app.file_sort == *key).then_some("当前"),
                            hint: None,
                        },
                    )
                })
                .collect()
        }
        PopupState::FileMenu { rel, is_dir, is_root } => {
            file_menu_entries(app, rel, *is_dir, *is_root, window)
        }
        PopupState::VaultMenu { root } => vault_menu_entries(app, root),
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::SortMenu => 160.0,
        // 文件/文件夹菜单 176；最近仓库行菜单 192
        PopupState::FileMenu { .. } => 176.0,
        PopupState::VaultMenu { .. } => 192.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 最近仓库行菜单：在系统文件管理器中打开 / 从列表移除（激活仓库行不可移除）。
/// 移除进二段确认 pane（只移出列表，不删磁盘文件）。
fn vault_menu_entries(app: &App, root: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let active = app
        .vault_root
        .as_ref()
        .map(|r| r.to_string_lossy().into_owned())
        .as_deref()
        == Some(root);
    let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
        (0, MenuEntry::action_icon(
            "在文件管理器中打开",
            "folder-open",
            Message::RevealVaultAt(root.to_string()),
        )),
        (0, MenuEntry::Divider),
        (0, MenuEntry::Action {
            label: "从列表移除".to_string(),
            icon: Some("unlink".into()),
            danger: true,
            disabled: active,
            on_press: (!active).then(|| Message::RemoveVaultAsk(root.to_string())),
            note: active.then_some("当前仓库"),
            hint: None,
        }),
    ];
    if app.remove_vault_confirm.as_deref() == Some(root) {
        let pane = column![text("只从列表移除，不影响磁盘文件")
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED)];
        entries.push((1, MenuEntry::Custom(pane.into(), 12.0 + 16.0 + 4.0)));
        entries.push((1, MenuEntry::action_icon(
            "确认移除",
            "unlink",
            Message::RemoveVaultConfirm(root.to_string()),
        )));
        entries.push((1, MenuEntry::action("取消", Message::RemoveVaultCancel)));
    }
    entries
}

/// 文件树右键菜单：文件夹 / 文件 / 根空白；删除 = 应用层确认流
/// （文件进确认 pane；文件夹先计数——空目录直接删、非空带条目数进确认 pane）。
/// 本机路径类菜单条目（复制绝对 / 相对路径、在系统文件管理器中显示）。
/// 协作空间的内容在服务端、没有本机路径，整组不出现；根行的相对路径无意义，不列。
/// 调用方按返回值是否为空决定是否插入分组分隔线（空组不留悬空线）。
fn path_menu_entries(
    app: &App,
    rel: &str,
    is_dir: bool,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        return Vec::new();
    }
    let mut out = vec![(
        0,
        MenuEntry::action_icon(
            "复制绝对路径",
            "link",
            Message::CopyEntryPath { rel: rel.to_string(), absolute: true },
        ),
    )];
    if !rel.is_empty() {
        out.push((
            0,
            MenuEntry::action_icon(
                "复制相对路径",
                "link",
                Message::CopyEntryPath { rel: rel.to_string(), absolute: false },
            ),
        ));
    }
    out.push((
        0,
        MenuEntry::action_icon(
            "在文件管理器中显示",
            "folder-open",
            Message::RevealEntry { rel: rel.to_string(), is_dir },
        ),
    ));
    out
}

fn file_menu_entries(
    app: &App,
    rel: &str,
    is_dir: bool,
    is_root: bool,
    window: iced::window::Id,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
    if is_dir {
        entries.push((0, MenuEntry::action_icon("新建画布", "palette", Message::DraftStart(rel.to_string(), crate::DraftKind::Canvas))));
        entries.push((0, MenuEntry::action_icon("新建笔记", "sticky-note",
            Message::DraftStart(rel.to_string(), DraftKind::Note))));
        entries.push((0, MenuEntry::action_icon("新建表格", "table", Message::DraftStart(rel.to_string(), crate::DraftKind::Table))));
        // 插件领域文档类型的新建入口（扩展名 / 标签 / 图标取自插件注册）
        for info in app.registry.doc_types() {
            let extension = info.registration.extension.clone();
            let label = format!("新建{}", info.registration.label);
            entries.push((
                0,
                MenuEntry::action_icon(
                    &label,
                    crate::plugin_actions::plugin_icon_key(
                        Some(&info.registration.icon),
                        "file-text",
                    ),
                    Message::DraftStart(
                        rel.to_string(),
                        DraftKind::Doc { extension, label: info.registration.label.clone() },
                    ),
                ),
            ));
        }
        entries.push((0, MenuEntry::action_icon("新建文件夹", "folder-plus",
            Message::DraftStart(rel.to_string(), DraftKind::Folder))));
        if !is_root {
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon(
                "图标颜色",
                "palette",
                Message::ColorPicker(crate::color_picker_actions::ColorPickerMsg::OpenFolder {
                    window,
                    rel: rel.to_string(),
                }),
            )));
            entries.push((0, MenuEntry::Divider));
            entries.push((0, MenuEntry::action_icon("创建副本", "copy",
                Message::DuplicateEntry(rel.to_string(), true))));
            entries.push((0, MenuEntry::Divider));
            let path_entries = path_menu_entries(app, rel, true);
            if !path_entries.is_empty() {
                entries.extend(path_entries);
                entries.push((0, MenuEntry::Divider));
            }
            entries.push((0, MenuEntry::action_icon("重命名", "pencil",
                Message::RenameStartFolder(rel.to_string()))));
            entries.push((0, MenuEntry::danger("删除", "bin",
                Message::FolderDeleteAsk(rel.to_string()))));
        } else {
            // 根行没有条目本身：只给本机路径类（相对路径无意义，由 helper 略去）
            let path_entries = path_menu_entries(app, rel, true);
            if !path_entries.is_empty() {
                entries.push((0, MenuEntry::Divider));
                entries.extend(path_entries);
            }
        }
    } else {
        let lower = rel.to_lowercase();
        if lower.ends_with(".canvas") {
            entries.push((0, MenuEntry::action_icon("转换为画布", "file-output", Message::NotImplemented("画布转换"))));
            entries.push((0, MenuEntry::Divider));
        }
        if lower.ends_with(".md") {
            // 标记状态决定条目形态（注册/注销）；副本不继承标记；viewer 只读降级（与 AI 设置同口径）
            let viewer = matches!(
                &app.repo_kind,
                crate::RepoKind::Space(s) if !s.role.can_write()
            );
            let marked = app.prompt_notes.iter().any(|n| n == rel);
            entries.push((0, MenuEntry::Action {
                label: if marked { "注销提示词" } else { "注册为提示词" }.into(),
                icon: Some(if marked { "bookmark-minus" } else { "bookmark-plus" }.into()),
                danger: false,
                disabled: viewer,
                on_press: (!viewer).then(|| Message::PromptToggle(rel.to_string())),
                note: None,
                hint: viewer.then(|| "当前角色为只读，无法修改提示词".to_string()),
            }));
            entries.push((0, MenuEntry::Divider));
        }
        entries.push((0, MenuEntry::action_icon("创建副本", "copy",
            Message::DuplicateEntry(rel.to_string(), false))));
        entries.push((0, MenuEntry::Divider));
        let path_entries = path_menu_entries(app, rel, false);
        if !path_entries.is_empty() {
            entries.extend(path_entries);
            entries.push((0, MenuEntry::Divider));
        }
        entries.push((0, MenuEntry::action_icon("重命名", "pencil",
            Message::RenameStartFile(rel.to_string()))));
        entries.push((0, MenuEntry::danger("删除", "bin",
            Message::DeleteAsk(rel.to_string()))));
    }

    // pane 1 = 删除就地确认（无标题行：说明文字 + 菜单项形态的确认/取消；
    // 内容来自应用层 delete_confirm——文件夹计数完成前 pane 1 不渲染）
    if !rel.is_empty() {
        if let Some((r, dir, count)) = app
            .delete_confirm
            .as_ref()
            .filter(|(cr, _, _)| cr == rel)
        {
            let mut height = 12.0 + 16.0 + 4.0;
            let mut pane_col = column![].spacing(2).padding(Padding::ZERO.vertical(6.0)).width(Length::Fill);
            if let Some(n) = count {
                pane_col = pane_col.push(
                    text(format!("文件夹包含 {n} 个文件/文件夹"))
                        .size(pal::fs_caption())
                        .color(pal::TEXT_SECONDARY),
                );
                height += 16.0;
            }
            pane_col = pane_col.push(
                text("删除后不可恢复")
                    .size(pal::fs_caption())
                    .color(pal::TEXT_MUTED),
            );
            entries.push((1, MenuEntry::Custom(pane_col.into(), height)));
            entries.push((1, MenuEntry::danger("确认删除", "bin",
                Message::DeleteEntry(r.to_string(), *dir))));
            entries.push((1, MenuEntry::action("取消", Message::DeleteCancel)));
        }
    }
    entries
}
