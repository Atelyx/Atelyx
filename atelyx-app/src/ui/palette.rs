//! 深色主题调色板与度量常量：颜色、字号、圆角与布局尺寸的统一定义。
//!
//! 单位约定：颜色用 sRGB 8bit/alpha；尺寸为 logical px；字号 / 圆角为固定档位。

use iced::{Color, Theme, theme::Palette};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, a)
}

// ===== 背景色阶（sunken < primary < secondary < tertiary < overlay）=====
/// 最深背景：内陷区域底（输入框 / 分段控件）
pub const BG_SUNKEN: Color = rgb(0x0c, 0x0d, 0x10);
/// 主背景：面板内容区
pub const BG_PRIMARY: Color = rgb(0x13, 0x14, 0x18);
/// 次背景：面板头 / 工具条 / 标题栏
pub const BG_SECONDARY: Color = rgb(0x1b, 0x1e, 0x23);
/// 三级背景：内嵌卡片 / 激活布局 tab 底
pub const BG_TERTIARY: Color = rgb(0x23, 0x27, 0x2e);
/// 浮层背景
pub const BG_OVERLAY: Color = rgb(0x2a, 0x2e, 0x36);

// ===== 遮罩 =====
/// 常规遮罩：小控件衬底（图片按钮圆底等），半透明黑
pub const SCRIM: Color = rgba(0, 0, 0, 0.6);
/// 强遮罩：灯箱全屏底，半透明黑
pub const SCRIM_STRONG: Color = rgba(0, 0, 0, 0.85);

// ===== 文本 =====
/// 主文字
pub const TEXT_PRIMARY: Color = rgb(0xe0, 0xdd, 0xd6);
/// 次要文字
pub const TEXT_SECONDARY: Color = rgb(0xab, 0xa9, 0xa2);
/// 弱化文字（占位 / 说明）
pub const TEXT_MUTED: Color = rgb(0x91, 0x8f, 0x88);

// ===== 边与交互态 =====
/// 最弱分隔线
pub const BORDER_SUBTLE: Color = rgb(0x25, 0x2a, 0x31);
/// 常规边框
pub const BORDER: Color = rgb(0x2e, 0x33, 0x3a);
/// 强边框
pub const BORDER_STRONG: Color = rgb(0x38, 0x3d, 0x44);
/// 悬停底色
pub const HOVER: Color = rgb(0x23, 0x27, 0x2e);

// ===== 表单 =====
/// 输入框占位文字色
pub const INPUT_PLACEHOLDER: Color = TEXT_MUTED;

// ===== 强调（黄铜金）=====
/// 强调色
pub const ACCENT: Color = rgb(0xe0, 0xa9, 0x4e);
/// 强调悬停（金提亮一档）。
pub const ACCENT_HOVER: Color = rgb(0xf0, 0xbc, 0x66);
/// 强调底上的前景（金底配深字）。
pub const ACCENT_FG: Color = rgb(0x14, 0x14, 0x14);
/// 强调淡底（激活态底色）。
pub const ACCENT_SOFT: Color = rgba(224, 169, 78, 0.14);

// ===== 状态语义色 =====
/// 成功
pub const SUCCESS: Color = rgb(0x5f, 0xbf, 0x8b);
/// 警告
pub const WARNING: Color = ACCENT;
/// 危险 / 错误
pub const DANGER: Color = rgb(0xe8, 0x77, 0x6c);

// ===== 日程色板 =====
/// 日程标记 7 色循环（`CalendarItem.color` 存对应 hex，顺序即循环序）
pub const EVENT_COLORS: [Color; 7] = [
    rgb(0xe0, 0x6c, 0x75),
    rgb(0x61, 0xaf, 0xef),
    rgb(0x98, 0xc3, 0x79),
    rgb(0xe5, 0xc0, 0x7b),
    rgb(0xc6, 0x78, 0xdd),
    rgb(0x56, 0xb6, 0xc2),
    rgb(0xd1, 0x9a, 0x66),
];

// ===== 标题栏窗口控制 =====
/// 标题栏按钮图标色
pub const TITLEBAR_ICON: Color = TEXT_SECONDARY;
/// 标题栏按钮悬停底色
pub const TITLEBAR_HOVER: Color = rgba(255, 255, 255, 0.07);
/// 关闭按钮悬停底色
pub const TITLEBAR_CLOSE_HOVER: Color = rgb(0xe8, 0x11, 0x23);

// ===== 形状与度量 =====
/// 圆角 xs
pub const RADIUS_XS: f32 = 4.0;
/// 圆角 sm
pub const RADIUS_SM: f32 = 6.0;
/// 圆角 md
pub const RADIUS_MD: f32 = 10.0;

/// 字阶档位（rem 语义：实际 px = 档位 × 界面基准字号 / 16）。
const FS_MICRO_REM: f32 = 11.0;
const FS_CAPTION_REM: f32 = 12.0;
const FS_UI_REM: f32 = 13.0;
const FS_BODY_REM: f32 = 14.0;
const FS_H2_REM: f32 = 16.0;
const FS_H1_REM: f32 = 20.0;

/// 界面基准字号（px，12–20；缺省 16）。进程单例：视图每帧取值，改值即全局生效。
/// 不经 App 状态：字号进 palette 函数而非逐控件传参，与字体句柄同为进程内单例。
static UI_BASE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(16);

/// 当前界面基准字号（px）。
pub fn ui_base() -> f32 {
    UI_BASE.load(std::sync::atomic::Ordering::Relaxed) as f32
}

/// 设置界面基准字号（px；调用方负责钳制 12–20）。
pub fn set_ui_base(px: u32) {
    UI_BASE.store(px, std::sync::atomic::Ordering::Relaxed);
}

fn font_scale() -> f32 {
    ui_base() / 16.0
}

/// 字阶 micro
pub fn fs_micro() -> f32 {
    FS_MICRO_REM * font_scale()
}
/// 字阶 caption
pub fn fs_caption() -> f32 {
    FS_CAPTION_REM * font_scale()
}
/// 字阶 ui（默认文字档）
pub fn fs_ui() -> f32 {
    FS_UI_REM * font_scale()
}
/// 字阶 body
pub fn fs_body() -> f32 {
    FS_BODY_REM * font_scale()
}
/// 字阶 h2
pub fn fs_h2() -> f32 {
    FS_H2_REM * font_scale()
}
/// 字阶 h1（占位引导标题档）
pub fn fs_h1() -> f32 {
    FS_H1_REM * font_scale()
}

/// 标题栏高度
pub const TITLEBAR_H: f32 = 36.0;
/// 状态栏高度
pub const STATUSBAR_H: f32 = 26.0;
/// 面板标签条高度
pub const TAB_BAR_H: f32 = 28.0;
/// 视口钳制边距
pub const VIEWPORT_GAP: f32 = 6.0;
/// 菜单项高度（12px 字行高 16 + 上下 6px 内边距）
pub const MENU_ITEM_H: f32 = 28.0;
/// 菜单分隔线高度（上下 4px + 1px 线）
pub const MENU_DIVIDER_H: f32 = 9.0;
/// 菜单分组头高度
pub const MENU_GROUP_H: f32 = 22.0;
/// 浮层容器上下内边距
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
