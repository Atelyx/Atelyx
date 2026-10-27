//! 功能模块（Pack）契约：视图元数据与注册表、宿主能力面、Pack 接口。
//! 只定义契约与装配期数据结构，不含宿主状态与渲染实现——宿主类型由装配根注入。

mod views;

use iced::window;
use iced::Element;

pub use views::{RegisterError, ViewDescriptor, ViewRegistry};

/// 宿主给 Pack 的能力面。
///
/// 只提供「拿到 core 子系统」的入口，不包装具体操作——core 有多少能力 Pack 就有多少；
/// 「每加一项操作就改宿主」是被禁止的扩张方向。方法随真实 Pack 的用例增长。
pub trait HostCtx {}

/// 一个功能模块：声明自己贡献哪些视图，并按 id 渲染。
pub trait Pack<H: HostCtx> {
    /// 本包的消息类型（视图事件经宿主的消息通道回到本包处理）。不需要自定义消息的包可用 `()`。
    type Msg: 'static;

    /// 贡献的视图元数据（顺序即标签菜单与视图选择器的呈现序）。
    fn views(&self) -> &[ViewDescriptor];

    /// 按 id 渲染；`id` 不属于本包时返回 `None`，由宿主继续问下一个包、最终落占位。
    fn view<'a>(&'a self, id: &str, host: &'a H, window: window::Id)
        -> Option<Element<'a, Self::Msg>>;
}
