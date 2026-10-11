//! UI 组件库：调色板 / 矢量图标 / 基础控件 / 统一弹层 / 菜单 / 通知。
//!
//! 视觉 token 与旧壳 CSS 变量一一对应（`palette`）；弹层与菜单是全应用
//! 唯一浮层入口，右键菜单、下拉、就地确认都经 [`popup`] + [`menu`] 组合。

pub mod controls;
pub mod icon;
pub mod icons;
pub mod menu;
pub mod notify;
pub mod palette;
pub mod popup;
pub mod svgpath;

/// 随应用分发的 IBM Plex Sans（SIL OFL 1.1）：400 / 500 / 600 三字重。
pub const FONT_REGULAR: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf");
pub const FONT_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf");
pub const FONT_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf");

/// 随应用分发的 Fraunces（SIL OFL 1.1，可变字重）：品牌名等展示字体。
pub const FONT_DISPLAY: &[u8] = include_bytes!("../../assets/fonts/Fraunces.ttf");

/// 应用图标（与旧壳同一来源的 PNG）。
pub const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");

/// 应用图标句柄（进程内单例：同一 Handle 复用纹理，避免每帧重上传）。
pub fn icon_handle() -> iced::widget::image::Handle {
    static HANDLE: std::sync::OnceLock<iced::widget::image::Handle> = std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| iced::widget::image::Handle::from_bytes(ICON_PNG.to_vec()))
        .clone()
}

/// 家族名（Font::with_name 与字重选择共用）。
pub const FONT_FAMILY: &str = "IBM Plex Sans";
/// 展示字体家族名（品牌名）。
pub const FONT_DISPLAY_FAMILY: &str = "Fraunces";

/// 展示字体 600（品牌名）。
pub fn display() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(FONT_DISPLAY_FAMILY),
        weight: iced::font::Weight::Semibold,
        ..iced::Font::default()
    }
}

/// 字重 600（标题/分组头等强调文字）。
pub fn semibold() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name(FONT_FAMILY),
        weight: iced::font::Weight::Semibold,
        ..iced::Font::default()
    }
}

/// ui 层上抛的应用消息（应用层统一转译）。
#[derive(Debug, Clone)]
pub enum UiMsg {
    /// 图标按钮悬停开始（label = 提示文案；box_px = 触发钮边长，提示锚在光标下方半边长 + 4 处）。
    HoverTip { window: iced::window::Id, label: String, box_px: f32 },
    /// 悬停结束（进入 120ms 收起延迟）。
    HoverTipEnd { window: iced::window::Id, label: String },
    /// 弹层钻入子面板 / 返回根面板。
    PopupPane(usize),
    /// 关闭弹层（外点/Esc/动作完成后）。
    PopupClose { window: iced::window::Id },
    /// 关闭一条通知。
    NotifyDismiss(u64),
}
