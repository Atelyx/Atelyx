//! Atelyx 界面组件库：调色板 / 矢量图标 / 字体 / 基础控件 / 统一弹层 / 菜单 / 通知 / 滚动容器。
//!
//! 视觉 token 定义于 `palette`；弹层与菜单是全应用唯一浮层入口，
//! 右键菜单、下拉、就地确认都经 [`popup`] + [`menu`] 组合。组件对宿主消息类型泛型，库内不定义业务消息。

pub mod color_picker;
pub mod controls;
pub mod font;
pub mod icon;
mod icons;
pub mod menu;
pub mod notify;
pub mod palette;
pub mod popup;
pub mod scroll;
mod svgpath;

pub use font::{bold, epoch, family, italic, mono, regular, set_family, text};

/// 宿主消息契约：组件库内部产生的界面消息（悬停提示、弹层导航等）经此包进宿主消息类型。
pub trait UiHost: Clone + 'static {
    fn ui(msg: UiMsg<Self>) -> Self;
}

/// 组件库上抛的界面消息（`M` 为宿主消息类型，由调用方统一转译）。
#[derive(Debug, Clone)]
pub enum UiMsg<M> {
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
    InputEdit { edit: Box<M>, arm: Box<M> },
    /// 设置输入提交（Enter 触发）：解除武装后执行提交。
    InputSubmit { commit: Box<M> },
    /// 外点收口：提交最近一次武装的输入（未武装 = no-op）。
    CommitPendingInput,
}
