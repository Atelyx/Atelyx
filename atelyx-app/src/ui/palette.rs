//! 深色主题调色板与度量常量：token 值与旧壳 `styles/index.css` 深色变量一一对应。
//!
//! 单位约定：颜色用 sRGB 8bit/alpha；尺寸为 logical px；字号/圆角对应旧壳
//! `--fs-*` / `--radius-*` 档位。

use iced::{Color, Theme, theme::Palette};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, a)
}

// ===== 背景色阶（sunken < primary < secondary < tertiary < overlay）=====
/// `--bg-sunken`
pub const BG_SUNKEN: Color = rgb(0x0c, 0x0d, 0x10);
/// `--bg-primary`：面板内容区
pub const BG_PRIMARY: Color = rgb(0x13, 0x14, 0x18);
/// `--bg-secondary`：面板头 / 工具条 / 标题栏
pub const BG_SECONDARY: Color = rgb(0x1b, 0x1e, 0x23);
/// `--bg-tertiary`：内嵌卡片 / 激活布局 tab 底
pub const BG_TERTIARY: Color = rgb(0x23, 0x27, 0x2e);
/// `--bg-overlay`：浮层
pub const BG_OVERLAY: Color = rgb(0x2a, 0x2e, 0x36);

// ===== 文本 =====
/// `--text-primary`
pub const TEXT_PRIMARY: Color = rgb(0xe0, 0xdd, 0xd6);
/// `--text-secondary`
pub const TEXT_SECONDARY: Color = rgb(0xab, 0xa9, 0xa2);
/// `--text-muted`
pub const TEXT_MUTED: Color = rgb(0x91, 0x8f, 0x88);

// ===== 边与交互态 =====
/// `--border-subtle`
pub const BORDER_SUBTLE: Color = rgb(0x25, 0x2a, 0x31);
/// `--border`
pub const BORDER: Color = rgb(0x2e, 0x33, 0x3a);
/// `--border-strong`
pub const BORDER_STRONG: Color = rgb(0x38, 0x3d, 0x44);
/// `--hover`
pub const HOVER: Color = rgb(0x23, 0x27, 0x2e);

// ===== 表单 =====
/// `--input-placeholder`
pub const INPUT_PLACEHOLDER: Color = TEXT_MUTED;

// ===== 强调（黄铜金）=====
/// `--accent`
pub const ACCENT: Color = rgb(0xe0, 0xa9, 0x4e);
/// `--accent-soft`
pub const ACCENT_SOFT: Color = rgba(224, 169, 78, 0.14);

// ===== 状态语义色 =====
/// `--success`
pub const SUCCESS: Color = rgb(0x5f, 0xbf, 0x8b);
/// `--warning`
pub const WARNING: Color = ACCENT;
/// `--danger`
pub const DANGER: Color = rgb(0xe8, 0x77, 0x6c);

// ===== 标题栏窗口控制 =====
/// `--titlebar-icon`
pub const TITLEBAR_ICON: Color = TEXT_SECONDARY;
/// `--titlebar-hover`
pub const TITLEBAR_HOVER: Color = rgba(255, 255, 255, 0.07);
/// `--titlebar-close-hover`
pub const TITLEBAR_CLOSE_HOVER: Color = rgb(0xe8, 0x11, 0x23);

// ===== 形状与度量 =====
/// `--radius-xs`
pub const RADIUS_XS: f32 = 4.0;
/// `--radius-sm`
pub const RADIUS_SM: f32 = 6.0;
/// `--radius-md`
pub const RADIUS_MD: f32 = 10.0;

/// 字阶：`--fs-micro`
pub const FS_MICRO: f32 = 11.0;
/// 字阶：`--fs-caption`
pub const FS_CAPTION: f32 = 12.0;
/// 字阶：`--fs-ui`（默认文字档）
pub const FS_UI: f32 = 13.0;
/// 字阶：`--fs-body`
pub const FS_BODY: f32 = 14.0;
/// 字阶：`--fs-h2`
pub const FS_H2: f32 = 16.0;

/// 标题栏高度（旧壳 `h-9`）
pub const TITLEBAR_H: f32 = 36.0;
/// 状态栏高度（旧壳 `h-[26px]`）
pub const STATUSBAR_H: f32 = 26.0;
/// 面板标签条高度（旧壳 `h-7`）
pub const TAB_BAR_H: f32 = 28.0;
/// 面板标签定宽（拖拽命中与重排序的几何基准，保持与既有几何一致）
pub const TAB_W: f32 = 96.0;
/// 视口钳制边距（旧壳 `VIEWPORT_GAP`）
pub const VIEWPORT_GAP: f32 = 6.0;
/// 菜单项高度（12px 字行高 16 + 上下 6px 内边距）
pub const MENU_ITEM_H: f32 = 28.0;
/// 菜单分隔线高度（上下 4px + 1px 线）
pub const MENU_DIVIDER_H: f32 = 9.0;
/// 菜单分组头高度
pub const MENU_GROUP_H: f32 = 22.0;
/// 浮层容器上下内边距（旧壳默认 `py-1`）
pub const POPUP_PAD_Y: f32 = 4.0;

/// 内置 iced 主题：按本调色板定制（内置部件的 palette 色 = accent 等语义映射）。
pub fn app_theme() -> Theme {
    Theme::custom(
        String::from("Atelyx Dark"),
        Palette {
            background: BG_PRIMARY,
            text: TEXT_PRIMARY,
            primary: ACCENT,
            success: SUCCESS,
            warning: WARNING,
            danger: DANGER,
        },
    )
}
