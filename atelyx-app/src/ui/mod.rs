//! UI 组件库：调色板 / 矢量图标 / 基础控件 / 统一弹层 / 菜单 / 通知。
//!
//! 视觉 token 定义于 `palette`；弹层与菜单是全应用
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

/// 应用图标（随应用分发的 PNG 资源）。
pub const ICON_PNG: &[u8] = include_bytes!("../../assets/icons/icon.png");

/// 应用图标句柄（进程内单例：同一 Handle 复用纹理，避免每帧重上传）。
pub fn icon_handle() -> iced::widget::image::Handle {
    static HANDLE: std::sync::OnceLock<iced::widget::image::Handle> = std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| iced::widget::image::Handle::from_bytes(ICON_PNG.to_vec()))
        .clone()
}

/// 窗口图标（任务栏 / Alt-Tab / 窗口切换器）；构造失败回退系统默认图标。
pub fn window_icon() -> Option<iced::window::icon::Icon> {
    iced::window::icon::from_file_data(ICON_PNG, None).ok()
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

/// 等宽字体（计数 / 键串 / 扩展名标签）。
pub fn mono() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Monospace,
        ..iced::Font::with_name(FONT_FAMILY)
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
    /// 设置输入逐键编辑：转发编辑值，并武装失焦提交消息（Enter 与外点共用该提交）。
    InputEdit { edit: Box<crate::Message>, arm: Box<crate::Message> },
    /// 设置输入提交（Enter 触发）：解除武装后执行提交。
    InputSubmit { commit: Box<crate::Message> },
    /// 外点收口：提交最近一次武装的输入（未武装 = no-op）。
    CommitPendingInput,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌图标资源损坏（重新编码出错 / 文件被换）须在测试里暴露，而不是等建窗时才表现成无图标。
    #[test]
    fn embedded_icon_png_decodes_into_window_icon() {
        assert!(window_icon().is_some());
    }
}
