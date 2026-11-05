//! 协作房间整面帧：连接状态与设置入口、空间成员名册、在线设备卡片。
//! 事实全部来自宿主快照，这里只排布与合并后的展示。

use atelyx_plugin_protocol::method::SpaceMemberRole;
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor, TextWeight};
use atelyx_plugin_sdk::frame_ui::{BoxStyle, BoxWidth, Edges, TextRole, TextStyle, Ui};

use crate::model::{Device, Model, Palette};

/// 内容区四周内衬与块间距。
const CONTENT_PAD_X: f32 = 12.0;
const CONTENT_PAD_Y: f32 = 10.0;
const CONTENT_GAP: f32 = 6.0;
/// 区块标题的内衬（与设置页分组头同档：左缩进 8、下留 8）。
const GROUP_PAD_X: f32 = 8.0;
const GROUP_PAD_BOTTOM: f32 = 8.0;
const GROUP_BAR_W: f32 = 3.0;
const GROUP_BAR_H: f32 = 11.0;
const GROUP_GAP: f32 = 8.0;
/// 成员卡片与设备卡片的内衬。
const CARD_PAD_X: f32 = 10.0;
const CARD_PAD_Y: f32 = 6.0;
/// 卡片内行间距。
const CARD_LINE_GAP: f32 = 2.0;
const DEVICE_LINE_GAP: f32 = 3.0;
/// 在线色点边长与昵称间距。
const DOT: f32 = 8.0;
const HEAD_GAP: f32 = 6.0;
/// 徽标内衬（角色 / 「我」）。
const BADGE_PAD_X: f32 = 6.0;
const BADGE_PAD_Y: f32 = 1.0;
/// 打开文件行：图标边长 / 图标与文字间距 / 行内衬。
const FILE_ICON: f32 = 11.0;
const FILE_GAP: f32 = 4.0;
const FILE_PAD_X: f32 = 4.0;
const FILE_PAD_Y: f32 = 1.0;
/// 连接状态行的图标边长与行间距。
const NET_ICON: f32 = 13.0;
const NET_GAP: f32 = 6.0;
/// 设置入口：图标边长、图标与文字间距、按钮内衬。
const SETTINGS_ICON: f32 = 12.0;
const SETTINGS_GAP: f32 = 4.0;
const SETTINGS_PAD_X: f32 = 8.0;
const SETTINGS_PAD_Y: f32 = 3.0;

/// 「协作设置」入口的命中区 id。
const SETTINGS: &str = "space-settings";
/// 空态里的「协作空间」入口命中区 id。
const OPEN_MENU: &str = "space-open-menu";

pub fn frame(ui: &mut Ui<'_, '_>, model: &Model) {
    // 首份回执未到：先画加载态，绝不先露空态
    if model.room.is_none() {
        if let Some(error) = model.room_error.as_ref() {
            failure_state(ui, error, &model.palette);
        } else {
            ui.loading_state();
        }
        return;
    }
    if !model.in_space() {
        empty_state(ui);
        return;
    }
    ui.scroll("room", 0.0, move |page| {
        page.pad(CONTENT_PAD_X, CONTENT_PAD_Y, move |area| {
            area.column(CONTENT_GAP, move |col| {
                if let Some(error) = model.room_error.as_ref() {
                    col.notice(error.clone(), ColorToken::Danger);
                }
                header(col, model);
                group_head(col, "空间成员", &format!("{} 人", model.members().len()));
                roster(col, model);
                group_head(col, "在线设备", &format!("{} 台在线", model.online_count()));
                for (index, device) in model.devices().into_iter().enumerate() {
                    device_row(col, model.gen, index, &device, &model.palette);
                }
                if model.room.as_ref().is_some_and(|room| room.peers.is_empty()) {
                    let label = if model.connected() {
                        "当前空间没有其他设备在线"
                    } else {
                        "连接中…"
                    };
                    col.text_styled(label, TextStyle::new(TextRole::Micro));
                }
            });
        });
    });
}

/// 连接状态行：状态图标 + 文案 + 末端的「协作设置」入口。
fn header(ui: &mut Ui<'_, '_>, model: &Model) {
    let connected = model.connected();
    let (icon, label, color) = if connected {
        ("wifi", "已连接", ColorToken::Success)
    } else {
        ("wifi-off", "未连接", ColorToken::TextMuted)
    };
    let radius = model.palette.radii.sm;
    let label_w = ui.text_width("协作设置", TextRole::Micro);
    let button_w = SETTINGS_ICON + SETTINGS_GAP + label_w + SETTINGS_PAD_X * 2.0;
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, None, 0.0),
        Edges {
            left: 0.0,
            right: 0.0,
            top: 2.0,
            bottom: 2.0,
        },
        0.0,
        move |area| {
            area.row(NET_GAP, move |line| {
                line.icon_named(icon, NET_ICON, color);
                line.text_styled(label, TextStyle::new(TextRole::Caption).color(color));
                line.grow();
                line.click_box(
                    SETTINGS,
                    BoxWidth::Fixed(button_w),
                    Edges::symmetric(SETTINGS_PAD_X, SETTINGS_PAD_Y),
                    radius,
                    false,
                    true,
                    |button| {
                        button.row(SETTINGS_GAP, |content| {
                            content.icon_named("settings", SETTINGS_ICON, ColorToken::TextSecondary);
                            content.text_styled(
                                "协作设置",
                                TextStyle::new(TextRole::Micro)
                                    .color(ColorToken::TextSecondary),
                            );
                        });
                    },
                );
            });
        },
    );
}

/// 区块标题：竖条 + 标题 + 右侧说明（计数 / 状态）。
fn group_head(ui: &mut Ui<'_, '_>, label: &str, hint: &str) {
    let (label, hint) = (label.to_string(), hint.to_string());
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, None, 0.0),
        Edges {
            left: GROUP_PAD_X,
            right: GROUP_PAD_X,
            top: 0.0,
            bottom: GROUP_PAD_BOTTOM,
        },
        0.0,
        move |area| {
            area.row(GROUP_GAP, move |line| {
                line.boxed(
                    BoxWidth::Fixed(GROUP_BAR_W),
                    BoxStyle::new(Some(ColorToken::BorderStrong), None, 2.0),
                    Edges::all(0.0),
                    2.0,
                    move |bar| bar.spacer(GROUP_BAR_H),
                );
                line.text_styled_fit(
                    label.clone(),
                    TextStyle::new(TextRole::Micro)
                        .weight(TextWeight::SemiBold)
                        .color(ColorToken::TextMuted),
                );
                line.grow();
                line.text_styled_fit(
                    hint.clone(),
                    TextStyle::new(TextRole::Micro).mono().color(ColorToken::TextMuted),
                );
            });
        },
    );
}

/// 成员名册：失败给可读原因；有缓存就列卡片；否则给加载 / 空提示。
fn roster(ui: &mut Ui<'_, '_>, model: &Model) {
    if let Some(error) = model.members.error.as_ref() {
        ui.text_styled(
            format!("成员名册加载失败：{error}"),
            TextStyle::new(TextRole::Micro).color(ColorToken::Danger),
        );
        return;
    }
    if model.members.ready {
        for member in model.members() {
            member_card(ui, member, &model.palette);
        }
        return;
    }
    let label = if model.members.loading { "加载中…" } else { "暂无名册" };
    ui.text_styled(label, TextStyle::new(TextRole::Micro));
}

/// 成员卡片：显示名 + 角色徽标一行，@username 次行。
fn member_card(
    ui: &mut Ui<'_, '_>,
    member: &atelyx_plugin_protocol::method::SpaceMember,
    palette: &Palette,
) {
    let radius = palette.radii.md;
    let (name, username) = (member.display_name.clone(), member.username.clone());
    let (role, role_color) = role_of(member.role, palette);
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::Surface), None, radius),
        Edges::symmetric(CARD_PAD_X, CARD_PAD_Y),
        radius,
        move |card| {
            card.column(CARD_LINE_GAP, move |lines| {
                lines.row(HEAD_GAP, move |head| {
                    head.text_styled_fit(
                        name.clone(),
                        TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
                    );
                    head.grow();
                    badge(head, role, role_color, palette.radii.xs);
                });
                lines.text_styled(format!("@{username}"), TextStyle::new(TextRole::Micro));
            });
        },
    );
}

/// 角色徽标文案与颜色（所有者用强调色，编辑者次级色，只读弱化色）。
fn role_of(role: SpaceMemberRole, palette: &Palette) -> (&'static str, PaintColor) {
    match role {
        SpaceMemberRole::Owner => ("所有者", parse_color(&palette.accent)),
        SpaceMemberRole::Editor => ("编辑者", parse_color(&palette.text_secondary)),
        SpaceMemberRole::Viewer => ("只读", parse_color(&palette.text_muted)),
    }
}

/// 在线设备卡片：色点 + 昵称（自己标「我」）一行，平台 · 版本次行，打开文件可点。
fn device_row(
    ui: &mut Ui<'_, '_>,
    gen: u64,
    device_index: usize,
    device: &Device,
    palette: &Palette,
) {
    let radius = palette.radii.md;
    let dot = parse_color(&device.color);
    let mut head = device.nickname.clone();
    if head.trim().is_empty() {
        head = "用户".into();
    }
    let meta = device_meta_line(device.platform.as_deref(), device.version.as_deref());
    let files = device.files.clone();
    let is_self = device.is_self;
    let accent = parse_color(&palette.accent);
    let radii = (palette.radii.sm, palette.radii.xs);
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::Surface), None, radius),
        Edges::symmetric(CARD_PAD_X, CARD_PAD_Y),
        radius,
        move |card| {
            card.column(DEVICE_LINE_GAP, move |lines| {
                lines.row(HEAD_GAP, move |line| {
                    line.canvas_sized(DOT, DOT, move |canvas| {
                        let area = canvas.area();
                        canvas.draw(DrawCommand::Circle {
                            cx: area.x + area.w / 2.0,
                            cy: area.y + area.h / 2.0,
                            r: area.w / 2.0,
                            fill: Some(dot),
                            stroke: None,
                            stroke_width: None,
                        });
                    });
                    line.text_styled_fit(
                        head.clone(),
                        TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
                    );
                    if is_self {
                        badge(line, "我", accent, radii.1);
                    }
                });
                lines.text_styled(meta.clone(), TextStyle::new(TextRole::Micro));
                if files.is_empty() {
                    lines.text_styled("暂无打开文件", TextStyle::new(TextRole::Micro));
                } else {
                    for (index, file) in files.iter().enumerate() {
                        let id = device_file_id(gen, device_index, index);
                        let hint_id = id.clone();
                        let (path, icon) = (file.path.clone(), file.icon.clone());
                        lines.click_box(
                            id,
                            BoxWidth::Fill,
                            Edges::symmetric(FILE_PAD_X, FILE_PAD_Y),
                            radii.1,
                            false,
                            true,
                            move |row| {
                                row.row(FILE_GAP, move |line| {
                                    line.icon_named(&icon, FILE_ICON, ColorToken::TextSecondary);
                                    let name = file_name(&path);
                                    line.elide_with_hint(
                                        &hint_id,
                                        name,
                                        path.clone(),
                                        TextStyle::new(TextRole::Micro)
                                            .color(ColorToken::TextSecondary),
                                    );
                                });
                            },
                        );
                    }
                }
            });
        },
    );
}

/// 设备卡片里一行打开文件的命中区 id：带快照世代与两级行号（路径可能超出命中区 id 字节上限）。
fn device_file_id(gen: u64, device: usize, file: usize) -> String {
    format!("file:{gen}:{device}:{file}")
}

/// 文件名（路径末段）。
fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 平台 · 版本展示行：旧端未上报的字段显式标注未知。
fn device_meta_line(platform: Option<&str>, version: Option<&str>) -> String {
    let platform = platform
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(platform_label)
        .unwrap_or_else(|| "未知平台".into());
    let version = version.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("未知版本");
    format!("{platform} · {version}")
}

/// 平台标识 → 展示名（未知标识原样透出，缺失由调用方标注「未知平台」）。
fn platform_label(token: &str) -> String {
    match token.to_ascii_lowercase().as_str() {
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        "android" => "Android".into(),
        "macos" => "macOS".into(),
        other => other.to_string(),
    }
}

/// 小徽标：成员色的 14% 淡底 + 同色文字（角色 / 「我」）。
fn badge(ui: &mut Ui<'_, '_>, label: &str, color: PaintColor, radius: f32) {
    let size = ui.role_size(TextRole::Micro);
    let width = ui.text_width(label, TextRole::Micro) + BADGE_PAD_X * 2.0;
    let height = ui.text_height(TextRole::Micro) + BADGE_PAD_Y * 2.0;
    let fill = tint(color);
    let label = label.to_string();
    ui.canvas_sized(width, height, move |canvas| {
        let area = canvas.area();
        canvas.draw(DrawCommand::Rect {
            x: area.x,
            y: area.y,
            w: area.w,
            h: area.h,
            fill: Some(fill),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(radius),
            shadow: None,
        });
        let (_, text_h) = canvas.measure(&label, size);
        canvas.text(
            &label,
            area.x + BADGE_PAD_X,
            area.y + (area.h - text_h) / 2.0,
            size,
            color,
        );
    });
}

/// 空态：未进入协作空间，附「协作空间」入口（点开宿主菜单）。
fn empty_state(ui: &mut Ui<'_, '_>) {
    ui.empty_state_action(
        "users",
        "进入协作空间",
        "登录并进入空间后显示在线成员。",
        OPEN_MENU,
        "协作空间",
    );
}

/// 房间快照读取失败且手上没有快照：可读原因 + 重试。
fn failure_state(ui: &mut Ui<'_, '_>, error: &str, palette: &Palette) {
    let error = error.to_string();
    let radius = palette.radii.md;
    ui.fill_column(0.0, move |outer| {
        outer.fill_spacer();
        outer.center_column(8.0, move |center| {
            center.boxed(
                BoxWidth::Fixed(40.0),
                BoxStyle::new(Some(ColorToken::Surface), None, radius),
                Edges::all(0.0),
                0.0,
                move |tile| {
                    tile.spacer(11.0);
                    tile.row(0.0, move |line| {
                        line.grow();
                        line.icon_named("triangle-alert", 18.0, ColorToken::TextMuted);
                        line.grow();
                    });
                    tile.spacer(11.0);
                },
            );
            center.text_styled_fit(
                "协作房间读取失败",
                TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
            );
            center.text_styled(
                error.clone(),
                TextStyle::new(TextRole::Label).color(ColorToken::TextMuted),
            );
            center.button(crate::RETRY, "重试");
        });
        outer.fill_spacer();
    });
}

/// `#rrggbb` / `#rrggbbaa` → 绘制色；非法输入回落中灰（不透明）。
fn parse_color(value: &str) -> PaintColor {
    let hex = value.trim_start_matches('#');
    let parse = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap_or(0x80);
    let rgba = match hex.len() {
        6 if hex.is_ascii() => [parse(0..2), parse(2..4), parse(4..6), 255],
        8 if hex.is_ascii() => [parse(0..2), parse(2..4), parse(4..6), parse(6..8)],
        _ => [0x80, 0x80, 0x80, 255],
    };
    PaintColor::Rgba { rgba }
}

/// 徽标淡底：同色 14% 不透明。
fn tint(color: PaintColor) -> PaintColor {
    match color {
        PaintColor::Rgba { rgba } => {
            let alpha = (0.14_f32 * 255.0).round() as u8;
            PaintColor::Rgba { rgba: [rgba[0], rgba[1], rgba[2], alpha] }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_line_marks_missing_fields_unknown() {
        assert_eq!(device_meta_line(Some("windows"), Some("0.6.1")), "Windows · 0.6.1");
        assert_eq!(device_meta_line(Some("linux"), None), "Linux · 未知版本");
        assert_eq!(device_meta_line(None, Some("0.6.1")), "未知平台 · 0.6.1");
        assert_eq!(device_meta_line(None, None), "未知平台 · 未知版本");
        assert_eq!(device_meta_line(Some("  "), Some("")), "未知平台 · 未知版本");
    }

    #[test]
    fn unknown_platform_token_passes_through() {
        assert_eq!(platform_label("freebsd"), "freebsd");
        assert_eq!(platform_label("macOS"), "macOS");
    }

    #[test]
    fn file_rows_use_the_path_tail() {
        assert_eq!(file_name("notes/方案.md"), "方案.md");
        assert_eq!(file_name("root.md"), "root.md");
    }
}
