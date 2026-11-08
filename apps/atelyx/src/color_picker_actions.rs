//! 取色器编排：打开弹层、预览与提交（表格字色/底色、文件夹图标色、协作展示色）。

use atelyx_core::table_ops::{self, OptState};
use iced::{Color, Element, Task};

use crate::layout_view::{PopupOpen, PopupState};
use crate::table_view::TableMsg;
use crate::ui::color_picker::{self, Hsv};
use crate::{App, Message};

/// 表格字色预设（与表格样式的既有色值一致）。
const TEXT_PRESETS: [&str; 10] =
    ["#e05252", "#e07b39", "#c99a16", "#4fae6a", "#2fa3b3", "#4f8fd0", "#8a5ad0", "#d94f9c", "#8a6d4a", "#8a8a8a"];
/// 表格底色预设（深色系，与深色主题的单元格底色协调）。
const BG_PRESETS: [&str; 10] =
    ["#4a2c2f", "#4a3626", "#494328", "#2c4031", "#244248", "#273b55", "#372d4c", "#492c40", "#3f362b", "#35383d"];
/// 文件夹图标色预设。
const FOLDER_PRESETS: [&str; 8] =
    ["#e05252", "#e07b39", "#e0b436", "#4fae6a", "#2f9e8f", "#4f8fd0", "#7a6fd0", "#c05fa8"];

/// 取色目标：提交动作按目标分派。
#[derive(Clone, Debug)]
pub(crate) enum ColorTarget {
    Table { bg: bool },
    Folder { rel: String },
    Collab,
}

#[derive(Clone, Debug)]
pub(crate) enum ColorPickerMsg {
    /// 文件夹右键「图标颜色」入口。
    OpenFolder { window: iced::window::Id, rel: String },
    /// 设置-多人协作「协作展示色」入口。
    OpenCollab { window: iced::window::Id },
    /// 拖动中的颜色（只刷新取色器自身，不落盘）。
    Preview(Hsv),
    /// 松手 / 点预设色块：提交一次。
    Commit(Hsv),
    /// 十六进制输入（完整 6 位合法值即提交）。
    Draft(String),
    /// 十六进制输入按 Enter 提交（含 3 位简写）。
    Submit,
    /// 默认 / 恢复默认。
    Reset,
}

/// 打开表格字色 / 底色取色器（格式气泡按钮触发）。
pub(crate) fn open_table(app: &mut App, window: iced::window::Id, bg: bool) {
    open(app, window, ColorTarget::Table { bg });
}

pub(crate) fn update(app: &mut App, msg: ColorPickerMsg) -> Task<Message> {
    match msg {
        ColorPickerMsg::OpenFolder { window, rel } => {
            open(app, window, ColorTarget::Folder { rel });
            Task::none()
        }
        ColorPickerMsg::OpenCollab { window } => {
            open(app, window, ColorTarget::Collab);
            Task::none()
        }
        ColorPickerMsg::Preview(hsv) => {
            set_state(app, Some(hsv), Some(hsv.hex()));
            Task::none()
        }
        ColorPickerMsg::Commit(hsv) => {
            set_state(app, Some(hsv), Some(hsv.hex()));
            commit(app, Some(hsv.hex()))
        }
        ColorPickerMsg::Draft(text) => {
            let full = text.trim().trim_start_matches('#').len() == 6;
            match color_picker::parse_hex(&text) {
                Some(color) if full => {
                    let hex = color_picker::format_hex(color);
                    set_state(app, Some(Hsv::from_color(color)), Some(text));
                    commit(app, Some(hex))
                }
                _ => {
                    set_state(app, None, Some(text));
                    Task::none()
                }
            }
        }
        ColorPickerMsg::Submit => {
            let Some(text) = draft(app) else { return Task::none() };
            let Some(color) = color_picker::parse_hex(&text) else { return Task::none() };
            let hex = color_picker::format_hex(color);
            set_state(app, Some(Hsv::from_color(color)), Some(hex.clone()));
            commit(app, Some(hex))
        }
        ColorPickerMsg::Reset => {
            let Some(target) = target(app) else { return Task::none() };
            let base = default_color(&target);
            set_state(app, Some(Hsv::from_color(base)), Some(String::new()));
            commit(app, None)
        }
    }
}

/// 弹层内容：当前目标对应的预设列表 + 当前色。
pub(crate) fn panel<'a>(
    app: &App,
    target: &ColorTarget,
    hsv: Hsv,
    hex: &str,
) -> (Element<'a, Message>, f32) {
    let current = current_hex(app, target).and_then(|value| color_picker::parse_hex(&value));
    let resets = match target {
        ColorTarget::Table { .. } => Some("默认"),
        ColorTarget::Folder { .. } => Some("恢复默认"),
        ColorTarget::Collab => None,
    };
    let reset = resets.map(|label| (label, Message::ColorPicker(ColorPickerMsg::Reset)));
    color_picker::panel(
        presets(target),
        current,
        hsv,
        hex,
        reset,
        Message::ColorPicker(ColorPickerMsg::Submit),
        |hsv| Message::ColorPicker(ColorPickerMsg::Preview(hsv)),
        |hsv| Message::ColorPicker(ColorPickerMsg::Commit(hsv)),
        |text| Message::ColorPicker(ColorPickerMsg::Draft(text)),
    )
}

fn presets(target: &ColorTarget) -> &'static [&'static str] {
    match target {
        ColorTarget::Table { bg: false } => &TEXT_PRESETS,
        ColorTarget::Table { bg: true } => &BG_PRESETS,
        ColorTarget::Folder { .. } => &FOLDER_PRESETS,
        ColorTarget::Collab => &crate::space_actions::COLLAB_COLORS,
    }
}

fn default_color(target: &ColorTarget) -> Color {
    let hex = match target {
        ColorTarget::Table { bg: false } => TEXT_PRESETS[0],
        ColorTarget::Table { bg: true } => BG_PRESETS[0],
        ColorTarget::Folder { .. } => FOLDER_PRESETS[5],
        ColorTarget::Collab => crate::space_actions::DEFAULT_COLLAB_COLOR,
    };
    color_picker::parse_hex(hex).unwrap_or(Color::WHITE)
}

/// 打开取色器弹层：当前色取目标现值，无值（混合 / 未设）取该目标的默认起始色。
fn open(app: &mut App, window: iced::window::Id, target: ColorTarget) {
    let current = current_hex(app, &target);
    let base = current
        .as_deref()
        .and_then(color_picker::parse_hex)
        .unwrap_or_else(|| default_color(&target));
    let hex = current.unwrap_or_else(|| color_picker::format_hex(base));
    let point = app.cursor_for(window);
    app.dismiss_popup();
    app.popup = Some((
        window,
        PopupOpen {
            point,
            flip_y: None,
            state: PopupState::ColorPicker { target, hsv: Hsv::from_color(base), hex },
            min_width: 0.0,
        },
    ));
}

fn target(app: &App) -> Option<ColorTarget> {
    match &app.popup.as_ref()?.1.state {
        PopupState::ColorPicker { target, .. } => Some(target.clone()),
        _ => None,
    }
}

/// 取色器输入框当前草稿。
fn draft(app: &App) -> Option<String> {
    match &app.popup.as_ref()?.1.state {
        PopupState::ColorPicker { hex, .. } => Some(hex.clone()),
        _ => None,
    }
}

/// 更新弹层内的取色器状态（`hsv` 保留拖动中的色相；`hex` 为输入框草稿）。
fn set_state(app: &mut App, hsv: Option<Hsv>, hex: Option<String>) {
    let Some((_, popup)) = app.popup.as_mut() else { return };
    let PopupState::ColorPicker { hsv: ref mut state_hsv, hex: ref mut state_hex, .. } =
        popup.state
    else {
        return;
    };
    if let Some(hsv) = hsv {
        *state_hsv = hsv;
    }
    if let Some(hex) = hex {
        *state_hex = hex;
    }
}

/// 提交一次颜色（None = 清除）；文件夹与协作色与现值相同则跳过落盘。
fn commit(app: &mut App, hex: Option<String>) -> Task<Message> {
    let Some(target) = target(app) else { return Task::none() };
    let current = current_hex(app, &target);
    match target {
        ColorTarget::Table { bg } => {
            Task::done(Message::Table(TableMsg::SetColor { bg, color: hex }))
        }
        ColorTarget::Folder { rel } => {
            let unchanged = match (&hex, &current) {
                (Some(next), Some(current)) => next.eq_ignore_ascii_case(current),
                (None, None) => true,
                _ => false,
            };
            if unchanged {
                Task::none()
            } else {
                Task::done(Message::FolderColorSet(rel, hex))
            }
        }
        ColorTarget::Collab => {
            let Some(next) = hex else { return Task::none() };
            if current.is_some_and(|c| c.eq_ignore_ascii_case(&next)) {
                Task::none()
            } else {
                Task::done(Message::CollabColorSet(next))
            }
        }
    }
}

fn current_hex(app: &App, target: &ColorTarget) -> Option<String> {
    match target {
        ColorTarget::Table { bg } => {
            let session = app.table.as_ref()?;
            let region = table_ops::region_of(
                session.view.selection.as_ref(),
                &session.data.fields,
                &session.data.rows,
            )?;
            let summary =
                table_ops::style_summary(&session.data.fields, &session.data.rows, &region);
            let state = if *bg { &summary.bg } else { &summary.color };
            match state {
                OptState::Value(Some(value)) => Some(value.clone()),
                _ => None,
            }
        }
        ColorTarget::Folder { rel } => app.folder_colors.get(rel).cloned(),
        ColorTarget::Collab => app.global.collab_color.clone(),
    }
}
