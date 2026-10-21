//! Atelyx 插件 SDK：插件侧的连接与能力面封装。
//! 插件作者引此包即可：[`run_stdio`] 完成握手与事件分发，[`Context`] 提供全部能力面代理调用，
//! [`view_builders`] / [`canvas_builders`] 构建界面树与画布指令清单，[`presets`] 提供
//! 列表项 / 徽标 / 提示块 / tab 条等常用组合。协议类型统一从 `protocol` 模块取用。

pub mod canvas_builders;
pub mod connection;
pub mod context;
pub mod handler;
pub mod presets;
pub mod view_builders;

/// 协议类型再导出：插件代码只依赖本 crate。
pub use atelyx_plugin_protocol as protocol;
pub use connection::{run_stdio, ConnectError, Connection};
pub use context::Context;
pub use handler::PluginHandler;
pub use protocol::handshake::PluginInfo;
