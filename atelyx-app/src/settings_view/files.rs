//! 设置-文件与路径：排除文件夹与附件文件夹（仓库级，随仓库 config 走）。

use iced::widget::{column, container, scrollable};
use iced::{Element, Length, Padding};

use crate::settings_view::{group_head, setting_row};
use crate::ui::controls;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, Message, RepoKind};

pub fn view(app: &App) -> Element<'static, Message> {
    let space_mode = matches!(app.repo_kind, RepoKind::Space(_));
    if app.vault_root.is_none() && !space_mode {
        return container(controls::empty_state(
            "folder-tree",
            "未打开仓库",
            "打开仓库后配置",
            None,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }
    let exclude_draft = app
        .exclude_folders_draft
        .clone()
        .unwrap_or_else(|| app.exclude_folders.join(", "));
    let attach_draft = app
        .attachment_folder_draft
        .clone()
        .unwrap_or_else(|| app.attachment_folder.clone().unwrap_or_default());
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0));
    if space_mode {
        col = col.push(
            text("协作空间的文件与路径设置由服务端管理")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    }
    col = col.push(group_head("文件面板", "", true));
    col = col.push(setting_row(
        "排除文件夹",
        "逗号分隔；同名文件夹不在文件树显示（重开仓库生效）",
        if space_mode {
            controls::form_input_static("如：草稿, 归档", &exclude_draft, 200.0)
        } else {
            controls::form_input(
                "如：草稿, 归档",
                &exclude_draft,
                200.0,
                Message::ExcludeFoldersInput,
                Message::ExcludeFoldersCommit,
            )
        },
    ));
    col = col.push(group_head("附件", "", true));
    col = col.push(setting_row(
        "附件文件夹",
        "导入附件的默认目录（相对仓库根，可含子路径）",
        if space_mode {
            controls::form_input_static("assets", &attach_draft, 200.0)
        } else {
            controls::form_input(
                "assets",
                &attach_draft,
                200.0,
                Message::AttachmentFolderInput,
                Message::AttachmentFolderCommit,
            )
        },
    ));
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
