//! Atelyx 界面树：构建面 [`view_builders`] / [`presets`] 只依赖协议，插件与宿主共用；
//! 渲染面 `render` 由宿主启用 `render` 特性，把树解释成 iced 控件。

pub mod presets;
pub mod support;
pub mod view_builders;

#[cfg(feature = "render")]
pub mod render;

pub use view_builders::NodeId;
